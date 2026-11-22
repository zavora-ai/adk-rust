//! S19 (off the network): S1 run by this binary again inside the operating system's sandbox —
//! Seatbelt on macOS, bubblewrap on Linux — with the network denied, `HF_HUB_OFFLINE=1` and a
//! warm model cache, under the on-device profile (`on_device = true` in `[egress]`).
//!
//! The sandboxed run is a child process: `vision-scenarios --s19-child <report.json>`. It
//! reads the forty pages with PP-OCRv6 on the device, tries one direct connection to show
//! the operating system refuses it, and writes what it saw. It runs twice: once with the
//! network denied outright, and once with the only door an egress proxy whose policy admits
//! no host, so any connection attempt would be refused and recorded.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use adk_core::egress::EgressPolicy;
use adk_sandbox::egress_proxy::{EgressProxy, InMemoryEgressAudit};
use adk_sandbox::sandbox::{
    NetworkAccess, SandboxEnforcer, SandboxPolicyBuilder, get_enforcer, sandboxed_command,
};
use adk_vision::{EgressRecord, FetchConfig};
use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::Checks;
use crate::fixtures::{fixtures, models};
use crate::s1_s2::{Profile, run_desk};

/// The argument that makes this binary the sandboxed S1 run.
pub const CHILD_FLAG: &str = "--s19-child";
const LOCAL: &str = "pp-ocrv6-tiny";
/// A public address the sandboxed run tries to reach directly.
const CANARY: &str = "1.1.1.1:443";

/// The sandboxed run: S1 under the on-device profile, its report written to `report`.
pub async fn child(report: &Path) -> Result<()> {
    let run = run_desk(Profile::OnDevice).await?;
    let hard: BTreeSet<String> = run
        .pages
        .iter()
        .filter(|(page, _)| page.fields.iter().any(|f| f.hard.is_some()))
        .map(|(_, digest)| digest.to_string())
        .collect();
    let refusals: Vec<Value> = run
        .records
        .iter()
        .filter_map(|record| match record {
            EgressRecord::Refused(r) => Some(json!({
                "step": r.step,
                "reason": r.reason,
                "hosts": r.hosts,
                "source": r.source,
            })),
            _ => None,
        })
        .collect();
    let canary = match std::net::TcpStream::connect_timeout(
        &CANARY.parse().context("the canary address")?,
        Duration::from_secs(3),
    ) {
        Ok(_) => "connected".to_string(),
        Err(err) => format!("refused: {err}"),
    };
    let remote_calls = run.calls.iter().filter(|c| c.ext["backend"] != json!(LOCAL)).count();
    let value = json!({
        "hfHubOffline": std::env::var("HF_HUB_OFFLINE").unwrap_or_default(),
        "pages": run.pages.len(),
        "answers": run.answers,
        "fields": run.fields.len(),
        "localFields": run.fields.iter().filter(|f| f.backend == LOCAL).count(),
        "escalatedBlocks": run.escalated.values().sum::<usize>(),
        "calls": run.calls.len(),
        "remoteCalls": remote_calls,
        "released": run.released.len(),
        "refusals": refusals,
        "hardPages": hard,
        "canary": canary,
    });
    std::fs::write(report, serde_json::to_vec_pretty(&value)?)?;
    Ok(())
}

/// What one sandboxed run reported, or why it did not.
struct Sandboxed {
    report: Option<Value>,
    failure: Option<String>,
}

/// Runs this binary's S1 child under `enforcer` with `network`.
async fn sandboxed(
    enforcer: &dyn SandboxEnforcer,
    cache: &Path,
    network: NetworkAccess,
) -> Result<Sandboxed> {
    let exe = std::env::current_exe()?;
    let bin = exe.parent().context("the binary's directory")?;
    let scratch = tempfile::tempdir()?;
    let report = scratch.path().join("report.json");
    let tmp = scratch.path().join("tmp");
    std::fs::create_dir_all(&tmp)?;
    let mut policy = SandboxPolicyBuilder::new()
        .allow_read(bin)
        .allow_read(cache)
        .allow_read(fixtures())
        .allow_read_write(scratch.path())
        .network(network)
        .env("HF_HUB_OFFLINE", "1")
        .env("HF_HUB_CACHE", cache.display().to_string())
        .env("HOME", scratch.path().display().to_string())
        .env("TMPDIR", tmp.display().to_string())
        .env("PATH", "/usr/bin:/bin")
        .env("RUST_LOG", "warn");
    // Merged-/usr distributions reach the dynamic loader through links into /usr; bubblewrap
    // then needs the root read-only (adk-sandbox, "Platform Differences").
    if cfg!(target_os = "linux") {
        policy = policy.allow_read("/");
    }
    let output = sandboxed_command(
        enforcer,
        exe.as_os_str(),
        &[OsString::from(CHILD_FLAG), report.clone().into_os_string()],
        &policy.build(),
        scratch.path(),
    )?
    .stdin(std::process::Stdio::null())
    .output()
    .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(6).collect();
        return Ok(Sandboxed {
            report: None,
            failure: Some(format!(
                "the sandboxed run exited with {}: {}",
                output.status,
                tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
            )),
        });
    }
    let text = std::fs::read_to_string(&report).context("the sandboxed run's report")?;
    Ok(Sandboxed { report: Some(serde_json::from_str(&text)?), failure: None })
}

pub async fn run() -> Result<Checks> {
    let mut checks = Checks::default();
    // The parent warms the cache, as a person fetches models before going offline.
    models().await?;
    let cache = FetchConfig::from_env().cache_dir;
    let enforcer = match get_enforcer() {
        Ok(enforcer) => enforcer,
        Err(err) => {
            checks.check(&format!("the operating system's sandbox starts here ({err})"), false);
            return Ok(checks);
        }
    };
    checks.note(format!(
        "S1 in the {} sandbox with HF_HUB_OFFLINE=1, the on-device profile and the models cached",
        enforcer.name()
    ));

    let denied = sandboxed(enforcer.as_ref(), &cache, NetworkAccess::Denied).await?;
    let audit = Arc::new(InMemoryEgressAudit::new());
    let proxy = EgressProxy::builder(EgressPolicy::deny_all())
        .call_id("s19")
        .audit(audit.clone())
        .start_for(enforcer.as_ref())?;
    let proxied = sandboxed(enforcer.as_ref(), &cache, proxy.network()).await?;
    drop(proxy);

    for failure in [&denied.failure, &proxied.failure].into_iter().flatten() {
        checks.note(failure.clone());
    }
    let (Some(report), Some(through_proxy)) = (&denied.report, &proxied.report) else {
        checks.check("S1 ran inside the sandbox with the network denied", false);
        return Ok(checks);
    };

    checks.check(
        &format!(
            "S1 ran inside the {} sandbox with the network denied and HF_HUB_OFFLINE=1",
            enforcer.name()
        ),
        report["hfHubOffline"] == "1",
    );
    let canaries = [&report["canary"], &through_proxy["canary"]];
    checks.note(format!("a direct connection to {CANARY} from inside: {}", report["canary"]));
    checks.check(
        "a direct connection from inside the sandbox is refused by the operating system",
        canaries.iter().all(|c| c.as_str().is_some_and(|c| c.starts_with("refused"))),
    );
    let completed = report["answers"] == 40
        && report["remoteCalls"] == 0
        && report["escalatedBlocks"] == 0
        && report["localFields"] == report["fields"];
    checks.check(
        &format!(
            "the run completed on local backends: 40 pages read, {} vision calls, every one on the device",
            report["calls"]
        ),
        completed,
    );

    let refusals = report["refusals"].as_array().cloned().unwrap_or_default();
    let hard: BTreeSet<String> = report["hardPages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| d.as_str().map(str::to_string))
        .collect();
    let refused_from: BTreeSet<String> =
        refusals.iter().filter_map(|r| r["source"].as_str().map(str::to_string)).collect();
    let at_the_gate = refusals.iter().all(|r| {
        r["step"] == "destination"
            && r["reason"].as_str().is_some_and(|reason| reason.contains("on-device profile"))
    });
    if let Some(first) = refusals.first() {
        checks.note(format!("refusal: {}", first["reason"].as_str().unwrap_or_default()));
    }
    checks.check(
        &format!(
            "each of the {} escalation attempts was refused at the egress gate with its reason, and nothing was released",
            refusals.len()
        ),
        refusals.len() == 5 && refused_from == hard && at_the_gate && report["released"] == 0,
    );
    let records = audit.records();
    checks.check(
        "through an egress proxy that admits no host, the sandbox egress record is empty",
        records.is_empty() && through_proxy["answers"] == 40,
    );
    if !records.is_empty() {
        checks.note(format!("sandbox egress records: {records:?}"));
    }
    Ok(checks)
}

/// Whether `args` asks for the sandboxed S1 run, and where its report goes.
pub fn child_report(args: &[String]) -> Option<&OsStr> {
    let at = args.iter().position(|a| a == CHILD_FLAG)?;
    args.get(at + 1).map(OsStr::new)
}
